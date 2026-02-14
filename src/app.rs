use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CEvent, EventStream, KeyCode, KeyEvent,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{ExecutableCommand, execute, terminal};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::actions::{ActionId, ActionKeyMap};
use crate::config::{AppConfig, CommandContext, CustomCommand};
use crate::domain::{RepoSnapshot, Revision};
use crate::hg::{
    CliHgClient, CommandResult, CustomInvocation, HgAction, HgClient, SnapshotOptions,
};
use crate::ui;

const LOG_LIMIT: usize = 200;
const MAX_LOG_LINES: usize = 300;
const DOUBLE_CLICK_THRESHOLD_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Files,
    Revisions,
    Bookmarks,
    Shelves,
    Conflicts,
    Log,
}

impl FocusPanel {
    pub fn all() -> [Self; 6] {
        [
            Self::Files,
            Self::Revisions,
            Self::Bookmarks,
            Self::Shelves,
            Self::Conflicts,
            Self::Log,
        ]
    }
}

#[derive(Debug, Clone)]
pub enum InputPurpose {
    CommitMessage,
    CommitMessageInteractive,
    BookmarkName,
    ShelveName,
}

#[derive(Debug, Clone)]
pub struct InputState {
    pub title: String,
    pub value: String,
    pub purpose: InputPurpose,
}

#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    pub message: String,
    pub action: PendingRunAction,
}

#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct InteractiveCommitRequest {
    pub message: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum PendingRunAction {
    Hg(HgAction),
    Custom(CustomRunAction),
}

impl PendingRunAction {
    pub fn command_preview(&self) -> String {
        match self {
            Self::Hg(action) => action.command_preview(),
            Self::Custom(action) => action.invocation.command_preview(),
        }
    }

    fn show_output(&self) -> bool {
        match self {
            Self::Hg(_) => false,
            Self::Custom(action) => action.show_output,
        }
    }

    fn clears_commit_selection_on_success(&self) -> bool {
        matches!(self, Self::Hg(HgAction::Commit { .. }))
    }
}

#[derive(Debug, Clone)]
pub struct CustomRunAction {
    pub title: String,
    pub show_output: bool,
    pub invocation: CustomInvocation,
}

#[derive(Debug)]
pub enum AppEvent {
    SnapshotLoaded {
        preserve_details: bool,
        include_revisions: bool,
        result: Result<RepoSnapshot, String>,
    },
    DetailLoaded {
        request_id: u64,
        result: Result<String, String>,
    },
    ActionFinished {
        action_preview: String,
        show_output: bool,
        clear_commit_selection: bool,
        result: Result<CommandResult, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LastMouseClick {
    panel: FocusPanel,
    index: Option<usize>,
    button: MouseButton,
    at: Instant,
}

pub struct App {
    pub config: AppConfig,
    pub focus: FocusPanel,
    pub snapshot: RepoSnapshot,
    pub detail_text: String,
    pub details_scroll: usize,
    pub log_lines: Vec<String>,
    pub status_line: String,
    pub input: Option<InputState>,
    pub confirmation: Option<PendingConfirmation>,
    pub command_palette: Option<CommandPaletteState>,
    pub commit_file_selection: BTreeSet<String>,
    pub interactive_commit_request: Option<InteractiveCommitRequest>,
    pub should_quit: bool,
    pub files_idx: usize,
    pub rev_idx: usize,
    pub bookmarks_idx: usize,
    pub shelves_idx: usize,
    pub conflicts_idx: usize,
    pub log_idx: usize,
    pub files_offset: usize,
    pub rev_offset: usize,
    pub bookmarks_offset: usize,
    pub shelves_offset: usize,
    pub conflicts_offset: usize,
    pub ui_rects: ui::UiRects,
    last_refresh: Instant,
    detail_request_id: u64,
    last_mouse_click: Option<LastMouseClick>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    hg: Arc<dyn HgClient>,
    keymap: ActionKeyMap,
}

impl App {
    #[allow(dead_code)]
    pub fn new(config: AppConfig) -> Result<Self> {
        Self::new_with_startup_issues(config, Vec::new())
    }

    pub fn new_with_startup_issues(config: AppConfig, startup_issues: Vec<String>) -> Result<Self> {
        let cwd = std::env::current_dir().context("failed reading current directory")?;
        let status_line = format!(
            "Theme: {} | key overrides: {} | q to quit.",
            config.theme,
            config.keybinds.len()
        );
        let mut keymap_issues = Vec::new();
        let keymap = match ActionKeyMap::from_overrides(&config.keybinds) {
            Ok(map) => map,
            Err(issues) => {
                keymap_issues = issues;
                ActionKeyMap::from_overrides(&std::collections::HashMap::new())
                    .expect("default keymap builds")
            }
        };
        let hg = Arc::new(CliHgClient::new(cwd)) as Arc<dyn HgClient>;
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let mut app = Self {
            config,
            focus: FocusPanel::Files,
            snapshot: RepoSnapshot::default(),
            detail_text: "Loading…".to_string(),
            details_scroll: 0,
            log_lines: Vec::new(),
            status_line,
            input: None,
            confirmation: None,
            command_palette: None,
            commit_file_selection: BTreeSet::new(),
            interactive_commit_request: None,
            should_quit: false,
            files_idx: 0,
            rev_idx: 0,
            bookmarks_idx: 0,
            shelves_idx: 0,
            conflicts_idx: 0,
            log_idx: 0,
            files_offset: 0,
            rev_offset: 0,
            bookmarks_offset: 0,
            shelves_offset: 0,
            conflicts_offset: 0,
            ui_rects: ui::UiRects::default(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            detail_request_id: 0,
            last_mouse_click: None,
            event_tx,
            event_rx,
            hg,
            keymap,
        };

        for issue in startup_issues {
            app.append_log(format!("Config warning: {issue}"));
        }
        for issue in keymap_issues {
            app.append_log(format!("Keybinding warning: {issue}"));
        }

        if app.config.custom_commands.is_empty() {
            app.append_log("No custom commands configured.");
        } else {
            let lines: Vec<String> = app
                .config
                .custom_commands
                .iter()
                .map(|cmd| {
                    let context = match cmd.context {
                        crate::config::CommandContext::Repo => "repo",
                        crate::config::CommandContext::File => "file",
                        crate::config::CommandContext::Revision => "revision",
                    };
                    format!(
                        "Loaded custom command: {} ({}) [{}] => {}{}",
                        cmd.id,
                        cmd.title,
                        context,
                        cmd.command,
                        if cmd.needs_confirmation {
                            " [confirm]"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            for line in lines {
                app.append_log(line);
            }
        }

        Ok(app)
    }

    pub async fn run(&mut self) -> Result<()> {
        enable_raw_mode().context("failed enabling raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen, EnableMouseCapture)
            .context("failed entering alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).context("failed creating terminal")?;
        terminal.clear().ok();

        self.refresh_snapshot(false);
        self.refresh_detail_for_focus();

        let mut event_stream = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let run_result = loop {
            if let Some(request) = self.interactive_commit_request.take() {
                if let Err(err) = self.run_interactive_commit(&mut terminal, request) {
                    self.status_line = "Interactive commit failed.".to_string();
                    self.append_log(format!("Interactive commit error: {err}"));
                    self.set_detail_text(format!("Interactive commit error:\n{err}"));
                    let _ = self.resume_terminal(&mut terminal);
                } else {
                    self.refresh_snapshot(false);
                }
            }

            if let Err(err) = terminal.draw(|f| {
                let rects = ui::compute_ui_rects(f.area());
                self.ui_rects = rects;
                ui::render(f, self, &rects);
            }) {
                break Err(anyhow::anyhow!("terminal draw failed: {err}"));
            }
            if self.should_quit {
                break Ok(());
            }

            tokio::select! {
                _ = tick.tick() => {
                    self.periodic_refresh();
                }
                maybe_ui_event = event_stream.next() => {
                    if let Some(Ok(event)) = maybe_ui_event {
                        match event {
                            CEvent::Key(key) => self.handle_key(key),
                            CEvent::Mouse(mouse) => self.handle_mouse(mouse),
                            _ => {}
                        }
                    }
                }
                maybe_app_event = self.event_rx.recv() => {
                    if let Some(app_event) = maybe_app_event {
                        self.handle_app_event(app_event);
                    }
                }
            }
        };

        self.restore_terminal(terminal)?;
        run_result
    }

    fn suspend_terminal(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        disable_raw_mode().context("failed disabling raw mode")?;
        terminal
            .backend_mut()
            .execute(terminal::LeaveAlternateScreen)
            .context("failed leaving alternate screen")?;
        terminal
            .backend_mut()
            .execute(DisableMouseCapture)
            .context("failed disabling mouse capture")?;
        terminal.show_cursor().context("failed showing cursor")?;
        Ok(())
    }

    fn resume_terminal(&self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        enable_raw_mode().context("failed enabling raw mode")?;
        terminal
            .backend_mut()
            .execute(terminal::EnterAlternateScreen)
            .context("failed entering alternate screen")?;
        terminal
            .backend_mut()
            .execute(EnableMouseCapture)
            .context("failed enabling mouse capture")?;
        terminal.clear().context("failed clearing terminal")?;
        Ok(())
    }

    fn run_interactive_commit(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        request: InteractiveCommitRequest,
    ) -> Result<()> {
        self.suspend_terminal(terminal)?;
        let preview = if request.files.is_empty() {
            "hg commit -i -m <message>".to_string()
        } else {
            format!("hg commit -i -m <message> <{} files>", request.files.len())
        };
        self.append_log(format!("Running interactively: {preview}"));
        println!();
        println!(
            "easyHg interactive commit started. Complete prompts to continue. (message: {})",
            request.message
        );
        let mut command = std::process::Command::new("hg");
        command
            .arg("commit")
            .arg("-i")
            .arg("-m")
            .arg(&request.message);
        command.args(&request.files);
        command.stdin(std::process::Stdio::inherit());
        command.stdout(std::process::Stdio::inherit());
        command.stderr(std::process::Stdio::inherit());
        let status = command
            .status()
            .context("failed to execute interactive mercurial commit")?;

        self.resume_terminal(terminal)?;
        if status.success() {
            self.status_line = "Interactive commit completed.".to_string();
            self.append_log("OK: hg commit -i");
            self.commit_file_selection.clear();
        } else {
            self.status_line = "Interactive commit exited with error.".to_string();
            self.append_log(format!("FAILED: interactive commit exit status {status}"));
        }
        Ok(())
    }

    fn restore_terminal(&self, mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        disable_raw_mode().ok();
        terminal
            .backend_mut()
            .execute(terminal::LeaveAlternateScreen)
            .ok();
        terminal.backend_mut().execute(DisableMouseCapture).ok();
        terminal.show_cursor().ok();
        Ok(())
    }

    fn periodic_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_secs(7) {
            self.refresh_snapshot_with_mode(true, false);
        }
    }

    fn refresh_snapshot(&mut self, preserve_details: bool) {
        self.refresh_snapshot_with_mode(preserve_details, true);
    }

    fn refresh_snapshot_with_mode(&mut self, preserve_details: bool, include_revisions: bool) {
        self.last_refresh = Instant::now();
        self.status_line = "Refreshing repository state…".to_string();
        let tx = self.event_tx.clone();
        let hg = Arc::clone(&self.hg);
        let options = SnapshotOptions {
            revision_limit: LOG_LIMIT,
            include_revisions,
        };
        tokio::spawn(async move {
            let result = hg
                .refresh_snapshot(options)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::SnapshotLoaded {
                preserve_details,
                include_revisions,
                result,
            });
        });
    }

    fn refresh_detail_for_focus(&mut self) {
        self.details_scroll = 0;
        let request_id = self.detail_request_id.wrapping_add(1);
        self.detail_request_id = request_id;
        let tx = self.event_tx.clone();
        let hg = Arc::clone(&self.hg);
        match self.focus {
            FocusPanel::Files => {
                if let Some(file) = self.snapshot.files.get(self.files_idx) {
                    let file_path = file.path.clone();
                    tokio::spawn(async move {
                        let result = hg
                            .file_diff(&file_path)
                            .await
                            .map_err(|err| err.to_string());
                        let _ = tx.send(AppEvent::DetailLoaded { request_id, result });
                    });
                }
            }
            FocusPanel::Revisions => {
                if let Some(rev) = self.snapshot.revisions.get(self.rev_idx) {
                    let rev_num = rev.rev;
                    tokio::spawn(async move {
                        let result = hg
                            .revision_patch(rev_num)
                            .await
                            .map_err(|err| err.to_string());
                        let _ = tx.send(AppEvent::DetailLoaded { request_id, result });
                    });
                }
            }
            _ => {
                self.set_detail_text("Select a file or revision to view details.");
            }
        }
    }

    fn set_detail_text(&mut self, text: impl Into<String>) {
        self.detail_text = text.into();
        self.details_scroll = 0;
    }

    fn run_pending_action(&mut self, action: PendingRunAction) {
        let tx = self.event_tx.clone();
        let hg = Arc::clone(&self.hg);
        let action_preview = action.command_preview();
        let show_output = action.show_output();
        let clear_commit_selection = action.clears_commit_selection_on_success();
        self.status_line = format!("Running: {action_preview}");
        tokio::spawn(async move {
            let result = match action {
                PendingRunAction::Hg(hg_action) => hg
                    .run_action(&hg_action)
                    .await
                    .map_err(|err| err.to_string()),
                PendingRunAction::Custom(custom_action) => hg
                    .run_custom_command(&custom_action.invocation)
                    .await
                    .map_err(|err| err.to_string()),
            };
            let _ = tx.send(AppEvent::ActionFinished {
                action_preview,
                show_output,
                clear_commit_selection,
                result,
            });
        });
    }

    fn run_hg_action(&mut self, action: HgAction) {
        self.run_pending_action(PendingRunAction::Hg(action));
    }

    fn confirm_action(&mut self, action: PendingRunAction, message: impl Into<String>) {
        self.confirmation = Some(PendingConfirmation {
            action,
            message: message.into(),
        });
    }

    fn open_input(&mut self, purpose: InputPurpose, title: impl Into<String>) {
        self.input = Some(InputState {
            title: title.into(),
            value: String::new(),
            purpose,
        });
    }

    fn selected_revision(&self) -> Option<&Revision> {
        self.snapshot.revisions.get(self.rev_idx)
    }

    pub fn is_file_selected_for_commit(&self, path: &str) -> bool {
        self.commit_file_selection.contains(path)
    }

    pub fn selected_file_commit_count(&self) -> usize {
        self.commit_file_selection.len()
    }

    fn append_log(&mut self, line: impl Into<String>) {
        let now = Local::now().format("%H:%M:%S");
        self.log_lines.push(format!("[{now}] {}", line.into()));
        if self.log_lines.len() > MAX_LOG_LINES {
            let extra = self.log_lines.len() - MAX_LOG_LINES;
            self.log_lines.drain(0..extra);
        }
    }

    fn adjust_indexes(&mut self) {
        if self.files_idx >= self.snapshot.files.len() {
            self.files_idx = self.snapshot.files.len().saturating_sub(1);
        }
        if self.rev_idx >= self.snapshot.revisions.len() {
            self.rev_idx = self.snapshot.revisions.len().saturating_sub(1);
        }
        if self.bookmarks_idx >= self.snapshot.bookmarks.len() {
            self.bookmarks_idx = self.snapshot.bookmarks.len().saturating_sub(1);
        }
        if self.shelves_idx >= self.snapshot.shelves.len() {
            self.shelves_idx = self.snapshot.shelves.len().saturating_sub(1);
        }
        if self.conflicts_idx >= self.snapshot.conflicts.len() {
            self.conflicts_idx = self.snapshot.conflicts.len().saturating_sub(1);
        }
        if self.log_idx >= self.log_lines.len() {
            self.log_idx = self.log_lines.len().saturating_sub(1);
        }
        let current_paths = self
            .snapshot
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect::<std::collections::HashSet<_>>();
        self.commit_file_selection
            .retain(|path| current_paths.contains(path));
        self.ensure_visible(FocusPanel::Files);
        self.ensure_visible(FocusPanel::Revisions);
        self.ensure_visible(FocusPanel::Bookmarks);
        self.ensure_visible(FocusPanel::Shelves);
        self.ensure_visible(FocusPanel::Conflicts);
    }

    fn panel_len(&self, panel: FocusPanel) -> usize {
        match panel {
            FocusPanel::Files => self.snapshot.files.len(),
            FocusPanel::Revisions => self.snapshot.revisions.len(),
            FocusPanel::Bookmarks => self.snapshot.bookmarks.len(),
            FocusPanel::Shelves => self.snapshot.shelves.len(),
            FocusPanel::Conflicts => self.snapshot.conflicts.len(),
            FocusPanel::Log => self.log_lines.len(),
        }
    }

    fn panel_index(&self, panel: FocusPanel) -> usize {
        match panel {
            FocusPanel::Files => self.files_idx,
            FocusPanel::Revisions => self.rev_idx,
            FocusPanel::Bookmarks => self.bookmarks_idx,
            FocusPanel::Shelves => self.shelves_idx,
            FocusPanel::Conflicts => self.conflicts_idx,
            FocusPanel::Log => self.log_idx,
        }
    }

    fn set_panel_index(&mut self, panel: FocusPanel, index: usize) {
        match panel {
            FocusPanel::Files => self.files_idx = index,
            FocusPanel::Revisions => self.rev_idx = index,
            FocusPanel::Bookmarks => self.bookmarks_idx = index,
            FocusPanel::Shelves => self.shelves_idx = index,
            FocusPanel::Conflicts => self.conflicts_idx = index,
            FocusPanel::Log => self.log_idx = index,
        }
    }

    fn panel_offset(&self, panel: FocusPanel) -> usize {
        match panel {
            FocusPanel::Files => self.files_offset,
            FocusPanel::Revisions => self.rev_offset,
            FocusPanel::Bookmarks => self.bookmarks_offset,
            FocusPanel::Shelves => self.shelves_offset,
            FocusPanel::Conflicts => self.conflicts_offset,
            FocusPanel::Log => self.log_idx,
        }
    }

    fn set_panel_offset(&mut self, panel: FocusPanel, offset: usize) {
        match panel {
            FocusPanel::Files => self.files_offset = offset,
            FocusPanel::Revisions => self.rev_offset = offset,
            FocusPanel::Bookmarks => self.bookmarks_offset = offset,
            FocusPanel::Shelves => self.shelves_offset = offset,
            FocusPanel::Conflicts => self.conflicts_offset = offset,
            FocusPanel::Log => self.log_idx = offset,
        }
    }

    fn panel_rect(&self, panel: FocusPanel) -> ratatui::layout::Rect {
        self.ui_rects.panel_rect(panel)
    }

    fn panel_body_rows(&self, panel: FocusPanel) -> usize {
        let rect = self.panel_rect(panel);
        rect.height.saturating_sub(2) as usize
    }

    fn detail_body_rows(&self) -> usize {
        self.ui_rects.details.height.saturating_sub(2) as usize
    }

    pub fn detail_line_count(&self) -> usize {
        self.detail_text.split('\n').count()
    }

    pub fn key_for_action(&self, action: ActionId) -> &str {
        self.keymap.key_for_action(action).unwrap_or("?")
    }

    pub fn max_detail_scroll(&self) -> usize {
        let rows = self.detail_body_rows().max(1);
        self.detail_line_count().saturating_sub(rows)
    }

    fn detail_scroll_offset(&self) -> usize {
        self.details_scroll.min(self.max_detail_scroll())
    }

    fn ensure_visible(&mut self, panel: FocusPanel) {
        if panel == FocusPanel::Log {
            return;
        }

        let len = self.panel_len(panel);
        if len == 0 {
            self.set_panel_index(panel, 0);
            self.set_panel_offset(panel, 0);
            return;
        }

        let mut idx = self.panel_index(panel).min(len.saturating_sub(1));
        let mut offset = self.panel_offset(panel);
        let rows = self.panel_body_rows(panel).max(1);
        let max_offset = len.saturating_sub(rows);

        offset = offset.min(max_offset);
        if idx < offset {
            offset = idx;
        } else if idx >= offset + rows {
            offset = idx + 1 - rows;
        }
        offset = offset.min(max_offset);
        idx = idx.min(len.saturating_sub(1));

        self.set_panel_index(panel, idx);
        self.set_panel_offset(panel, offset);
    }

    fn panel_at(&self, x: u16, y: u16) -> Option<FocusPanel> {
        FocusPanel::all()
            .into_iter()
            .find(|panel| rect_contains(self.panel_rect(*panel), x, y))
    }

    fn list_row_from_point(&self, panel: FocusPanel, x: u16, y: u16) -> Option<usize> {
        if panel == FocusPanel::Log {
            return None;
        }

        let rect = self.panel_rect(panel);
        if rect.width <= 2 || rect.height <= 2 {
            return None;
        }
        let left = rect.x.saturating_add(1);
        let right_exclusive = rect.x.saturating_add(rect.width.saturating_sub(1));
        let top = rect.y.saturating_add(1);
        let bottom_exclusive = rect.y.saturating_add(rect.height.saturating_sub(1));
        let inside_body = x >= left && x < right_exclusive && y >= top && y < bottom_exclusive;
        if !inside_body {
            return None;
        }

        let relative = (y - top) as usize;
        let idx = self.panel_offset(panel).saturating_add(relative);
        if idx < self.panel_len(panel) {
            Some(idx)
        } else {
            None
        }
    }

    fn point_in_details(&self, x: u16, y: u16) -> bool {
        rect_contains(self.ui_rects.details, x, y)
    }

    fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::SnapshotLoaded {
                preserve_details,
                include_revisions,
                result,
            } => match result {
                Ok(mut snapshot) => {
                    if !include_revisions {
                        snapshot.revisions = self.snapshot.revisions.clone();
                    }
                    self.snapshot = snapshot;
                    self.adjust_indexes();
                    self.status_line = "Repository state refreshed.".to_string();
                    if !preserve_details {
                        self.refresh_detail_for_focus();
                    }
                    self.append_log("Snapshot refreshed");
                }
                Err(err) => {
                    self.status_line = "Snapshot refresh failed.".to_string();
                    self.append_log(format!("Refresh failed: {err}"));
                }
            },
            AppEvent::DetailLoaded { request_id, result } => {
                if request_id == self.detail_request_id {
                    match result {
                        Ok(text) => {
                            let detail_text = if text.trim().is_empty() {
                                "No diff output.".to_string()
                            } else {
                                text
                            };
                            self.set_detail_text(detail_text);
                        }
                        Err(err) => {
                            self.set_detail_text(format!("Failed loading detail: {err}"));
                        }
                    }
                }
            }
            AppEvent::ActionFinished {
                action_preview,
                show_output,
                clear_commit_selection,
                result,
            } => match result {
                Ok(out) => {
                    if out.success {
                        self.status_line = format!("Completed: {}", out.command_preview);
                        self.append_log(format!("OK: {}", out.command_preview));
                        if clear_commit_selection {
                            self.commit_file_selection.clear();
                        }
                        if show_output {
                            let text = collect_command_output(&out);
                            if !text.is_empty() {
                                self.set_detail_text(text);
                            }
                        }
                    } else {
                        self.status_line = format!("Command failed: {}", out.command_preview);
                        let detail = format!(
                            "{}\n{}\n{}",
                            out.command_preview,
                            out.stdout.trim(),
                            out.stderr.trim()
                        );
                        self.append_log(format!("FAILED: {}", detail.trim()));
                        self.set_detail_text(detail);
                    }
                    self.refresh_snapshot(false);
                }
                Err(err) => {
                    self.status_line = format!("Command error: {action_preview}");
                    self.append_log(format!("ERROR: {}", err.trim()));
                    self.set_detail_text(err);
                }
            },
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.handle_confirmation_key(key)
            || self.handle_input_key(key)
            || self.handle_command_palette_key(key)
        {
            return;
        }

        if let Some(action) = self.keymap.action_for_event(key) {
            self.dispatch_action(action);
        }
    }

    fn dispatch_action(&mut self, action: ActionId) {
        match action {
            ActionId::Quit => self.should_quit = true,
            ActionId::Help => self.append_log(help_text(
                &self.keymap,
                &self.snapshot.capabilities,
                !self.config.custom_commands.is_empty(),
            )),
            ActionId::FocusNext => self.cycle_focus(true),
            ActionId::FocusPrev => self.cycle_focus(false),
            ActionId::MoveDown => self.move_selection(1),
            ActionId::MoveUp => self.move_selection(-1),
            ActionId::RefreshSnapshot => self.refresh_snapshot(false),
            ActionId::RefreshDetails => self.refresh_detail_for_focus(),
            ActionId::OpenCustomCommands => self.open_command_palette(),
            ActionId::ToggleFileForCommit => self.toggle_selected_file_for_commit(),
            ActionId::ClearFileSelection => self.clear_file_selection(),
            ActionId::Commit => {
                let title = if self.selected_file_commit_count() == 0 {
                    "Commit message (all tracked changes)".to_string()
                } else {
                    format!(
                        "Commit message ({} selected file{})",
                        self.selected_file_commit_count(),
                        if self.selected_file_commit_count() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                };
                self.open_input(InputPurpose::CommitMessage, title);
            }
            ActionId::CommitInteractive => {
                let title = if self.selected_file_commit_count() == 0 {
                    "Interactive commit message (hg commit -i, all tracked changes)".to_string()
                } else {
                    format!(
                        "Interactive commit message (hg commit -i, {} selected file{})",
                        self.selected_file_commit_count(),
                        if self.selected_file_commit_count() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                };
                self.open_input(InputPurpose::CommitMessageInteractive, title);
            }
            ActionId::Bookmark => self.open_input(InputPurpose::BookmarkName, "New bookmark"),
            ActionId::Shelve => {
                if self.snapshot.capabilities.has_shelve {
                    self.open_input(InputPurpose::ShelveName, "Shelve name");
                } else {
                    self.status_line = "Shelve extension/command unavailable.".to_string();
                }
            }
            ActionId::Push => self.confirm_action(
                PendingRunAction::Hg(HgAction::Push),
                "Push current changes?",
            ),
            ActionId::Pull => self.run_hg_action(HgAction::Pull),
            ActionId::Incoming => self.run_hg_action(HgAction::Incoming),
            ActionId::Outgoing => self.run_hg_action(HgAction::Outgoing),
            ActionId::UpdateSelected => self.update_action_for_selection(),
            ActionId::UnshelveSelected => self.unshelve_selected(),
            ActionId::ResolveMark => self.mark_selected_conflict(true),
            ActionId::ResolveUnmark => self.mark_selected_conflict(false),
            ActionId::RebaseSelected => self.maybe_rebase(),
            ActionId::HisteditSelected => self.maybe_histedit(),
            ActionId::HardRefresh => {
                self.refresh_snapshot(false);
                self.refresh_detail_for_focus();
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.confirmation.is_some() || self.input.is_some() || self.command_palette.is_some() {
            return;
        }

        let hovered_panel = self.panel_at(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(panel) = hovered_panel {
                    let clicked_idx = self.list_row_from_point(panel, mouse.column, mouse.row);
                    let is_double = self.is_double_click(panel, clicked_idx, MouseButton::Left);
                    self.last_mouse_click = Some(LastMouseClick {
                        panel,
                        index: clicked_idx,
                        button: MouseButton::Left,
                        at: Instant::now(),
                    });

                    self.focus = panel;
                    if let Some(idx) = clicked_idx {
                        self.set_panel_index(panel, idx);
                        self.ensure_visible(panel);
                    }

                    if is_double && matches!(panel, FocusPanel::Files | FocusPanel::Revisions) {
                        self.refresh_detail_for_focus();
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if self.point_in_details(mouse.column, mouse.row) {
                    self.scroll_details(1);
                } else {
                    let panel = hovered_panel.unwrap_or(self.focus);
                    self.scroll_panel(panel, 1);
                }
            }
            MouseEventKind::ScrollUp => {
                if self.point_in_details(mouse.column, mouse.row) {
                    self.scroll_details(-1);
                } else {
                    let panel = hovered_panel.unwrap_or(self.focus);
                    self.scroll_panel(panel, -1);
                }
            }
            _ => {}
        }
    }

    fn is_double_click(
        &self,
        panel: FocusPanel,
        index: Option<usize>,
        button: MouseButton,
    ) -> bool {
        let Some(last) = self.last_mouse_click else {
            return false;
        };
        if last.panel != panel || last.index != index || last.button != button {
            return false;
        }
        last.at.elapsed() <= Duration::from_millis(DOUBLE_CLICK_THRESHOLD_MS)
    }

    fn scroll_panel(&mut self, panel: FocusPanel, delta: isize) {
        self.focus = panel;
        if panel == FocusPanel::Log {
            let len = self.log_lines.len();
            if len == 0 {
                self.log_idx = 0;
                return;
            }
            let current = self.log_idx as isize;
            let next = (current + delta).clamp(0, (len - 1) as isize);
            self.log_idx = next as usize;
            return;
        }

        let len = self.panel_len(panel);
        if len == 0 {
            self.set_panel_index(panel, 0);
            self.set_panel_offset(panel, 0);
            return;
        }

        let current = self.panel_index(panel) as isize;
        let next = (current + delta).clamp(0, (len - 1) as isize) as usize;
        self.set_panel_index(panel, next);
        self.ensure_visible(panel);
        if matches!(panel, FocusPanel::Files | FocusPanel::Revisions) {
            self.refresh_detail_for_focus();
        }
    }

    fn scroll_details(&mut self, delta: isize) {
        let current = self.detail_scroll_offset() as isize;
        let max = self.max_detail_scroll() as isize;
        let next = (current + delta).clamp(0, max);
        self.details_scroll = next as usize;
    }

    fn maybe_rebase(&mut self) {
        if !self.snapshot.capabilities.has_rebase {
            self.status_line = "Rebase extension not enabled.".to_string();
            return;
        }
        if let Some(rev) = self.selected_revision() {
            self.confirm_action(
                PendingRunAction::Hg(HgAction::RebaseSource {
                    source_rev: rev.rev,
                }),
                format!("Rebase revision {} onto working parent (.)?", rev.rev),
            );
        }
    }

    fn maybe_histedit(&mut self) {
        if !self.snapshot.capabilities.has_histedit {
            self.status_line = "Histedit extension not enabled.".to_string();
            return;
        }
        if let Some(rev) = self.selected_revision() {
            self.confirm_action(
                PendingRunAction::Hg(HgAction::HisteditBase { base_rev: rev.rev }),
                format!("Start histedit from revision {}?", rev.rev),
            );
        }
    }

    fn mark_selected_conflict(&mut self, resolved: bool) {
        if let Some(conflict) = self.snapshot.conflicts.get(self.conflicts_idx) {
            let action = if resolved {
                HgAction::ResolveMark {
                    path: conflict.path.clone(),
                }
            } else {
                HgAction::ResolveUnmark {
                    path: conflict.path.clone(),
                }
            };
            self.run_hg_action(action);
        } else {
            self.status_line = "No conflict selected.".to_string();
        }
    }

    fn unshelve_selected(&mut self) {
        if let Some(shelf) = self.snapshot.shelves.get(self.shelves_idx) {
            self.confirm_action(
                PendingRunAction::Hg(HgAction::Unshelve {
                    name: shelf.name.clone(),
                }),
                format!("Unshelve '{}'? This applies shelved changes.", shelf.name),
            );
        } else {
            self.status_line = "No shelf selected.".to_string();
        }
    }

    fn update_action_for_selection(&mut self) {
        match self.focus {
            FocusPanel::Bookmarks => {
                if let Some(bookmark) = self.snapshot.bookmarks.get(self.bookmarks_idx) {
                    self.confirm_action(
                        PendingRunAction::Hg(HgAction::UpdateToBookmark {
                            name: bookmark.name.clone(),
                        }),
                        format!("Update working directory to bookmark '{}'?", bookmark.name),
                    );
                }
            }
            _ => {
                if let Some(rev) = self.snapshot.revisions.get(self.rev_idx) {
                    self.confirm_action(
                        PendingRunAction::Hg(HgAction::UpdateToRevision { rev: rev.rev }),
                        format!("Update working directory to revision {}?", rev.rev),
                    );
                }
            }
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        let panels = FocusPanel::all();
        let pos = panels
            .iter()
            .position(|panel| *panel == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % panels.len()
        } else {
            (pos + panels.len() - 1) % panels.len()
        };
        self.focus = panels[next];
        self.refresh_detail_for_focus();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.focus == FocusPanel::Log {
            let len = self.log_lines.len();
            if len == 0 {
                self.log_idx = 0;
                return;
            }
            let current = self.log_idx as isize;
            let next = (current + delta).clamp(0, (len - 1) as isize);
            self.log_idx = next as usize;
            return;
        }

        let len = self.panel_len(self.focus);
        if len == 0 {
            self.set_panel_index(self.focus, 0);
            self.set_panel_offset(self.focus, 0);
            return;
        }

        let current = self.panel_index(self.focus) as isize;
        let next = (current + delta).clamp(0, (len - 1) as isize) as usize;
        self.set_panel_index(self.focus, next);
        self.ensure_visible(self.focus);
        if matches!(self.focus, FocusPanel::Files | FocusPanel::Revisions) {
            self.refresh_detail_for_focus();
        }
    }

    fn open_command_palette(&mut self) {
        if self.config.custom_commands.is_empty() {
            self.status_line = "No custom commands configured.".to_string();
            return;
        }
        self.command_palette = Some(CommandPaletteState { selected: 0 });
        self.status_line = "Custom commands: Enter run | Esc cancel.".to_string();
    }

    fn toggle_selected_file_for_commit(&mut self) {
        let Some(file) = self.snapshot.files.get(self.files_idx) else {
            self.status_line = "No file selected.".to_string();
            return;
        };
        let path = file.path.clone();
        if self.commit_file_selection.contains(&path) {
            self.commit_file_selection.remove(&path);
            self.status_line = format!("Removed from commit selection: {path}");
        } else {
            self.commit_file_selection.insert(path.clone());
            self.status_line = format!("Selected for commit: {path}");
        }
    }

    fn clear_file_selection(&mut self) {
        self.commit_file_selection.clear();
        self.status_line = "Cleared commit file selection.".to_string();
    }

    fn handle_command_palette_key(&mut self, key: KeyEvent) -> bool {
        if self.command_palette.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.command_palette = None;
                self.status_line = "Custom command selection cancelled.".to_string();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.config.custom_commands.len();
                if len > 0
                    && let Some(palette) = self.command_palette.as_mut()
                {
                    palette.selected = (palette.selected + 1).min(len - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(palette) = self.command_palette.as_mut()
                    && palette.selected > 0
                {
                    palette.selected -= 1;
                }
            }
            KeyCode::Enter => self.run_selected_custom_command(),
            _ => {}
        }
        true
    }

    fn run_selected_custom_command(&mut self) {
        let selected = self
            .command_palette
            .as_ref()
            .map(|palette| palette.selected)
            .unwrap_or(0);
        let Some(command) = self.config.custom_commands.get(selected).cloned() else {
            self.status_line = "No custom command selected.".to_string();
            self.command_palette = None;
            return;
        };
        self.command_palette = None;

        match self.prepare_custom_run_action(&command) {
            Ok(custom_action) => {
                let preview = custom_action.invocation.command_preview();
                let title = custom_action.title.clone();
                let pending = PendingRunAction::Custom(custom_action);
                if command.needs_confirmation {
                    self.confirm_action(
                        pending,
                        format!("Run custom command '{}'?\n{}", title, preview),
                    );
                } else {
                    self.run_pending_action(pending);
                }
            }
            Err(err) => {
                self.status_line = "Custom command not runnable.".to_string();
                self.append_log(format!("Custom command '{}' failed: {err}", command.id));
                self.set_detail_text(err);
            }
        }
    }

    fn prepare_custom_run_action(
        &self,
        command: &CustomCommand,
    ) -> Result<CustomRunAction, String> {
        let template_vars = self.custom_template_vars(command)?;
        let (program_raw, base_args_raw) = parse_command_parts(&command.command)?;
        let program = render_template(&program_raw, &template_vars);
        if program.trim().is_empty() {
            return Err(format!(
                "custom command '{}' resolved to empty program",
                command.id
            ));
        }

        let mut args = base_args_raw
            .iter()
            .map(|arg| render_template(arg, &template_vars))
            .collect::<Vec<_>>();
        args.extend(
            command
                .args
                .iter()
                .map(|arg| render_template(arg, &template_vars)),
        );

        let env = command
            .env
            .iter()
            .map(|(key, value)| (key.clone(), render_template(value, &template_vars)))
            .collect::<Vec<_>>();

        Ok(CustomRunAction {
            title: command.title.clone(),
            show_output: command.show_output,
            invocation: CustomInvocation { program, args, env },
        })
    }

    fn custom_template_vars(
        &self,
        command: &CustomCommand,
    ) -> Result<std::collections::HashMap<&'static str, String>, String> {
        let mut vars = std::collections::HashMap::new();
        let repo_root = self
            .snapshot
            .repo_root
            .clone()
            .ok_or_else(|| "repository root unavailable".to_string())?;
        vars.insert("repo_root", repo_root);
        vars.insert(
            "branch",
            self.snapshot
                .branch
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        );

        match command.context {
            CommandContext::Repo => {}
            CommandContext::File => {
                let file = self
                    .snapshot
                    .files
                    .get(self.files_idx)
                    .ok_or_else(|| "file-context command requires selected file".to_string())?;
                vars.insert("file", file.path.clone());
            }
            CommandContext::Revision => {
                let rev = self.snapshot.revisions.get(self.rev_idx).ok_or_else(|| {
                    "revision-context command requires selected revision".to_string()
                })?;
                vars.insert("rev", rev.rev.to_string());
                vars.insert("node", rev.node.clone());
            }
        }

        if let Some(file) = self.snapshot.files.get(self.files_idx) {
            vars.entry("file").or_insert_with(|| file.path.clone());
        }
        if let Some(rev) = self.snapshot.revisions.get(self.rev_idx) {
            vars.entry("rev").or_insert_with(|| rev.rev.to_string());
            vars.entry("node").or_insert_with(|| rev.node.clone());
        }

        Ok(vars)
    }

    fn handle_confirmation_key(&mut self, key: KeyEvent) -> bool {
        if self.confirmation.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                if let Some(confirm) = self.confirmation.take() {
                    self.run_pending_action(confirm.action);
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirmation = None;
                self.status_line = "Action cancelled.".to_string();
            }
            _ => {}
        }
        true
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> bool {
        if self.input.is_none() {
            return false;
        }

        let mut submit: Option<InputState> = None;
        if let Some(input) = self.input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.status_line = "Input cancelled.".to_string();
                }
                KeyCode::Enter => {
                    submit = self.input.clone();
                }
                KeyCode::Backspace => {
                    input.value.pop();
                }
                KeyCode::Char(c) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        input.value.push(c);
                    }
                }
                _ => {}
            }
        }

        if let Some(input) = submit {
            let value = input.value.trim();
            if value.is_empty() {
                self.status_line = "Input cannot be empty.".to_string();
                return true;
            }
            self.input = None;
            match input.purpose {
                InputPurpose::CommitMessage => {
                    let files = self
                        .commit_file_selection
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    self.run_hg_action(HgAction::Commit {
                        message: value.to_string(),
                        files,
                    });
                }
                InputPurpose::CommitMessageInteractive => {
                    let files = self
                        .commit_file_selection
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    self.interactive_commit_request = Some(InteractiveCommitRequest {
                        message: value.to_string(),
                        files,
                    });
                    self.status_line =
                        "Launching interactive commit; complete prompts in terminal.".to_string();
                }
                InputPurpose::BookmarkName => self.run_hg_action(HgAction::BookmarkCreate {
                    name: value.to_string(),
                }),
                InputPurpose::ShelveName => self.run_hg_action(HgAction::ShelveCreate {
                    name: value.to_string(),
                }),
            }
        }
        true
    }
}

fn parse_command_parts(raw: &str) -> Result<(String, Vec<String>), String> {
    #[derive(Clone, Copy)]
    enum QuoteMode {
        Single,
        Double,
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote_mode = None;
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote_mode {
            Some(QuoteMode::Single) => {
                if ch == '\'' {
                    quote_mode = None;
                } else {
                    current.push(ch);
                }
            }
            Some(QuoteMode::Double) => match ch {
                '"' => quote_mode = None,
                '\\' => {
                    let escaped = chars
                        .next()
                        .ok_or_else(|| "custom command has trailing escape".to_string())?;
                    current.push(escaped);
                }
                _ => current.push(ch),
            },
            None => match ch {
                '\'' => quote_mode = Some(QuoteMode::Single),
                '"' => quote_mode = Some(QuoteMode::Double),
                '\\' => {
                    let escaped = chars
                        .next()
                        .ok_or_else(|| "custom command has trailing escape".to_string())?;
                    current.push(escaped);
                }
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        parts.push(std::mem::take(&mut current));
                    }
                    while chars.peek().is_some_and(|peek| peek.is_whitespace()) {
                        chars.next();
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if quote_mode.is_some() {
        return Err("custom command has unmatched quote".to_string());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return Err("custom command has empty executable".to_string());
    }
    Ok((parts[0].clone(), parts[1..].to_vec()))
}

fn render_template(raw: &str, vars: &std::collections::HashMap<&'static str, String>) -> String {
    let mut rendered = raw.to_string();
    for (name, value) in vars {
        rendered = rendered.replace(&format!("{{{name}}}"), value);
    }
    rendered
}

fn collect_command_output(result: &CommandResult) -> String {
    let mut sections = Vec::new();
    if !result.stdout.trim().is_empty() {
        sections.push(format!("stdout:\n{}", result.stdout.trim_end()));
    }
    if !result.stderr.trim().is_empty() {
        sections.push(format!("stderr:\n{}", result.stderr.trim_end()));
    }
    sections.join("\n\n")
}

fn rect_contains(rect: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    let x_end = rect.x.saturating_add(rect.width);
    let y_end = rect.y.saturating_add(rect.height);
    x >= rect.x && x < x_end && y >= rect.y && y < y_end
}

fn help_text(
    keymap: &ActionKeyMap,
    caps: &crate::domain::HgCapabilities,
    has_custom_commands: bool,
) -> String {
    let key = |action: ActionId| keymap.key_for_action(action).unwrap_or("?");
    let mut text = vec![
        format!(
            "Keys: {} quit | {} focus+ | {} focus- | {} down | {} up | {} refresh | {} reload diff",
            key(ActionId::Quit),
            key(ActionId::FocusNext),
            key(ActionId::FocusPrev),
            key(ActionId::MoveDown),
            key(ActionId::MoveUp),
            key(ActionId::RefreshSnapshot),
            key(ActionId::RefreshDetails),
        ),
        format!(
            "Actions: {} pick file | {} clear picks | {} commit | {} interactive commit | {} bookmark | {} update | {} push(confirm) | {} pull",
            key(ActionId::ToggleFileForCommit),
            key(ActionId::ClearFileSelection),
            key(ActionId::Commit),
            key(ActionId::CommitInteractive),
            key(ActionId::Bookmark),
            key(ActionId::UpdateSelected),
            key(ActionId::Push),
            key(ActionId::Pull),
        ),
        format!(
            "Remote: {} incoming | {} outgoing",
            key(ActionId::Incoming),
            key(ActionId::Outgoing),
        ),
        format!(
            "Shelves: {} create shelf | {} unshelve selected shelf",
            key(ActionId::Shelve),
            key(ActionId::UnshelveSelected),
        ),
        format!(
            "Conflicts: {} mark resolved | {} mark unresolved",
            key(ActionId::ResolveMark),
            key(ActionId::ResolveUnmark),
        ),
        "Mouse: click focus/select | wheel scroll hovered panel or Details (fallback: focused panel) | double-click files/commits loads details".to_string(),
    ];
    if caps.has_rebase {
        text.push(format!(
            "History: {} rebase selected revision onto '.'",
            key(ActionId::RebaseSelected)
        ));
    }
    if caps.has_histedit {
        text.push(format!(
            "History: {} histedit from selected revision",
            key(ActionId::HisteditSelected)
        ));
    }
    if has_custom_commands {
        text.push(format!(
            "Custom: {} open command palette",
            key(ActionId::OpenCustomCommands)
        ));
    }
    text.join(" | ")
}

pub async fn run_app(config: AppConfig, startup_issues: Vec<String>) -> Result<()> {
    let mut app = App::new_with_startup_issues(config, startup_issues)?;
    app.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, CommandContext, CustomCommand};
    use ratatui::layout::Rect;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_app() -> App {
        let mut app = App::new(AppConfig::default()).expect("app");
        app.ui_rects = ui::UiRects {
            header: Rect::new(0, 0, 100, 2),
            footer: Rect::new(0, 29, 100, 1),
            files: Rect::new(0, 2, 58, 12),
            details: Rect::new(0, 14, 58, 15),
            revisions: Rect::new(58, 2, 42, 10),
            bookmarks: Rect::new(58, 12, 42, 5),
            shelves: Rect::new(58, 17, 21, 5),
            conflicts: Rect::new(79, 17, 21, 5),
            log: Rect::new(58, 22, 42, 7),
        };
        app
    }

    fn left_down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn scroll_down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn scroll_up(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn run_hg(repo: &Path, args: &[&str]) {
        let output = Command::new("hg")
            .current_dir(repo)
            .args(args)
            .output()
            .expect("spawn hg command");
        assert!(
            output.status.success(),
            "hg {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_repo_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("easyhg-details-e2e-{}-{nanos}", std::process::id()))
    }

    #[derive(Debug)]
    struct RecordingHgClient {
        snapshot: RepoSnapshot,
        calls: std::sync::Mutex<Vec<SnapshotOptions>>,
    }

    impl RecordingHgClient {
        fn new(snapshot: RepoSnapshot) -> Self {
            Self {
                snapshot,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<SnapshotOptions> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl HgClient for RecordingHgClient {
        async fn refresh_snapshot(&self, options: SnapshotOptions) -> anyhow::Result<RepoSnapshot> {
            self.calls.lock().expect("calls lock").push(options);
            Ok(self.snapshot.clone())
        }

        async fn file_diff(&self, _file: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn revision_patch(&self, _rev: i64) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn run_action(&self, _action: &HgAction) -> anyhow::Result<CommandResult> {
            Ok(CommandResult {
                command_preview: "mock".to_string(),
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn run_custom_command(
            &self,
            _invocation: &CustomInvocation,
        ) -> anyhow::Result<CommandResult> {
            Ok(CommandResult {
                command_preview: "mock".to_string(),
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn panel_hit_testing() {
        let app = make_app();
        assert_eq!(app.panel_at(1, 3), Some(FocusPanel::Files));
        assert_eq!(app.panel_at(80, 3), Some(FocusPanel::Revisions));
        assert_eq!(app.panel_at(90, 26), Some(FocusPanel::Log));
        assert_eq!(app.panel_at(20, 20), None);
    }

    #[test]
    fn row_mapping_uses_offset() {
        let mut app = make_app();
        app.snapshot.files = vec![
            crate::domain::FileChange {
                path: "a".to_string(),
                status: crate::domain::FileStatus::Modified,
            };
            20
        ];
        app.files_offset = 5;
        assert_eq!(app.list_row_from_point(FocusPanel::Files, 2, 3), Some(5));
        assert_eq!(app.list_row_from_point(FocusPanel::Files, 2, 4), Some(6));
    }

    #[test]
    fn mouse_click_selects_row_and_focuses_panel() {
        let mut app = make_app();
        app.snapshot.bookmarks = vec![
            crate::domain::Bookmark {
                name: "a".to_string(),
                rev: 1,
                node: "a".to_string(),
                active: false,
            },
            crate::domain::Bookmark {
                name: "b".to_string(),
                rev: 2,
                node: "b".to_string(),
                active: false,
            },
        ];

        app.handle_mouse(left_down(60, 13));
        assert_eq!(app.focus, FocusPanel::Bookmarks);
        assert_eq!(app.bookmarks_idx, 0);
    }

    #[test]
    fn modal_blocks_mouse() {
        let mut app = make_app();
        app.focus = FocusPanel::Files;
        app.confirmation = Some(PendingConfirmation {
            message: "Confirm".to_string(),
            action: PendingRunAction::Hg(HgAction::Push),
        });
        app.handle_mouse(left_down(80, 3));
        assert_eq!(app.focus, FocusPanel::Files);
    }

    #[test]
    fn mouse_scroll_falls_back_to_focused_panel_when_not_over_panel() {
        let mut app = make_app();
        app.focus = FocusPanel::Bookmarks;
        app.snapshot.bookmarks = vec![
            crate::domain::Bookmark {
                name: "a".to_string(),
                rev: 1,
                node: "a".to_string(),
                active: false,
            },
            crate::domain::Bookmark {
                name: "b".to_string(),
                rev: 2,
                node: "b".to_string(),
                active: false,
            },
        ];

        app.handle_mouse(scroll_down(1, 1));
        assert_eq!(app.focus, FocusPanel::Bookmarks);
        assert_eq!(app.bookmarks_idx, 1);
    }

    #[test]
    fn mouse_scroll_prefers_hovered_panel_over_focused_panel() {
        let mut app = make_app();
        app.focus = FocusPanel::Conflicts;
        app.conflicts_idx = 1;
        app.snapshot.bookmarks = vec![
            crate::domain::Bookmark {
                name: "a".to_string(),
                rev: 1,
                node: "a".to_string(),
                active: false,
            },
            crate::domain::Bookmark {
                name: "b".to_string(),
                rev: 2,
                node: "b".to_string(),
                active: false,
            },
        ];
        app.snapshot.conflicts = vec![
            crate::domain::ConflictEntry {
                resolved: false,
                path: "x".to_string(),
            },
            crate::domain::ConflictEntry {
                resolved: false,
                path: "y".to_string(),
            },
        ];

        app.handle_mouse(scroll_down(60, 13));
        assert_eq!(app.focus, FocusPanel::Bookmarks);
        assert_eq!(app.bookmarks_idx, 1);
        assert_eq!(app.conflicts_idx, 1);
    }

    #[test]
    fn mouse_scroll_on_details_uses_details_offset_without_changing_focus() {
        let mut app = make_app();
        app.focus = FocusPanel::Files;
        app.files_idx = 2;
        app.detail_text = (0..30)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        app.handle_mouse(scroll_down(2, 15));
        assert_eq!(app.details_scroll, 1);
        assert_eq!(app.focus, FocusPanel::Files);
        assert_eq!(app.files_idx, 2);

        app.handle_mouse(scroll_up(2, 15));
        assert_eq!(app.details_scroll, 0);
    }

    #[test]
    fn detail_scroll_resets_when_new_detail_arrives() {
        let mut app = make_app();
        app.details_scroll = 5;
        app.detail_request_id = 99;

        app.handle_app_event(AppEvent::DetailLoaded {
            request_id: 99,
            result: Ok("new detail text".to_string()),
        });

        assert_eq!(app.details_scroll, 0);
    }

    #[test]
    fn periodic_snapshot_refresh_preserves_detail_scroll_for_same_target() {
        let mut app = make_app();
        app.focus = FocusPanel::Files;
        app.files_idx = 0;
        app.snapshot.files = vec![crate::domain::FileChange {
            path: "src/main.rs".to_string(),
            status: crate::domain::FileStatus::Modified,
        }];
        app.detail_text = (0..30)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.details_scroll = 7;

        app.handle_app_event(AppEvent::SnapshotLoaded {
            preserve_details: true,
            include_revisions: true,
            result: Ok(RepoSnapshot {
                files: vec![crate::domain::FileChange {
                    path: "src/main.rs".to_string(),
                    status: crate::domain::FileStatus::Modified,
                }],
                ..RepoSnapshot::default()
            }),
        });

        assert_eq!(app.details_scroll, 7);
    }

    #[test]
    fn periodic_snapshot_refresh_resets_detail_scroll_when_target_changes() {
        let mut app = make_app();
        app.focus = FocusPanel::Files;
        app.files_idx = 0;
        app.snapshot.files = vec![crate::domain::FileChange {
            path: "src/main.rs".to_string(),
            status: crate::domain::FileStatus::Modified,
        }];
        app.details_scroll = 7;

        app.handle_app_event(AppEvent::SnapshotLoaded {
            preserve_details: true,
            include_revisions: true,
            result: Ok(RepoSnapshot::default()),
        });

        assert_eq!(app.details_scroll, 7);
    }

    #[test]
    fn explicit_snapshot_refresh_resets_detail_scroll() {
        let mut app = make_app();
        app.focus = FocusPanel::Files;
        app.files_idx = 0;
        app.snapshot.files = vec![crate::domain::FileChange {
            path: "src/main.rs".to_string(),
            status: crate::domain::FileStatus::Modified,
        }];
        app.details_scroll = 7;

        app.handle_app_event(AppEvent::SnapshotLoaded {
            preserve_details: false,
            include_revisions: true,
            result: Ok(RepoSnapshot::default()),
        });

        assert_eq!(app.details_scroll, 0);
    }

    #[test]
    fn lightweight_snapshot_refresh_preserves_existing_revisions() {
        let mut app = make_app();
        app.snapshot.revisions = vec![crate::domain::Revision {
            rev: 7,
            node: "abc".to_string(),
            desc: "old".to_string(),
            user: "u".to_string(),
            branch: "default".to_string(),
            phase: "draft".to_string(),
            tags: Vec::new(),
            bookmarks: Vec::new(),
            date_unix_secs: 0,
        }];

        app.handle_app_event(AppEvent::SnapshotLoaded {
            preserve_details: true,
            include_revisions: false,
            result: Ok(RepoSnapshot::default()),
        });

        assert_eq!(app.snapshot.revisions.len(), 1);
        assert_eq!(app.snapshot.revisions[0].rev, 7);
    }

    #[test]
    fn full_snapshot_refresh_replaces_revisions() {
        let mut app = make_app();
        app.snapshot.revisions = vec![crate::domain::Revision {
            rev: 7,
            node: "abc".to_string(),
            desc: "old".to_string(),
            user: "u".to_string(),
            branch: "default".to_string(),
            phase: "draft".to_string(),
            tags: Vec::new(),
            bookmarks: Vec::new(),
            date_unix_secs: 0,
        }];

        app.handle_app_event(AppEvent::SnapshotLoaded {
            preserve_details: true,
            include_revisions: true,
            result: Ok(RepoSnapshot {
                revisions: vec![crate::domain::Revision {
                    rev: 8,
                    node: "def".to_string(),
                    desc: "new".to_string(),
                    user: "u".to_string(),
                    branch: "default".to_string(),
                    phase: "draft".to_string(),
                    tags: Vec::new(),
                    bookmarks: Vec::new(),
                    date_unix_secs: 0,
                }],
                ..RepoSnapshot::default()
            }),
        });

        assert_eq!(app.snapshot.revisions.len(), 1);
        assert_eq!(app.snapshot.revisions[0].rev, 8);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn periodic_refresh_uses_lightweight_snapshot_mode() {
        let mut app = make_app();
        let client = Arc::new(RecordingHgClient::new(RepoSnapshot::default()));
        app.hg = client.clone();
        app.last_refresh = Instant::now() - Duration::from_secs(8);

        app.periodic_refresh();
        let snapshot_event = tokio::time::timeout(Duration::from_secs(3), app.event_rx.recv())
            .await
            .expect("snapshot timeout")
            .expect("snapshot event");
        app.handle_app_event(snapshot_event);

        let calls = client.calls();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].include_revisions);
        assert_eq!(calls[0].revision_limit, LOG_LIMIT);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manual_refresh_uses_full_snapshot_mode() {
        let mut app = make_app();
        let client = Arc::new(RecordingHgClient::new(RepoSnapshot::default()));
        app.hg = client.clone();

        app.refresh_snapshot(false);
        let snapshot_event = tokio::time::timeout(Duration::from_secs(3), app.event_rx.recv())
            .await
            .expect("snapshot timeout")
            .expect("snapshot event");
        app.handle_app_event(snapshot_event);

        let calls = client.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].include_revisions);
        assert_eq!(calls[0].revision_limit, LOG_LIMIT);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn e2e_mouse_scroll_reaches_details_bottom_and_survives_periodic_refresh() {
        if Command::new("hg").arg("--version").output().is_err() {
            eprintln!("skipping e2e test: hg binary not available");
            return;
        }

        let repo_dir = temp_repo_dir();
        fs::create_dir_all(&repo_dir).expect("create temp repo directory");

        run_hg(&repo_dir, &["init"]);
        fs::write(repo_dir.join("big.txt"), "base\n").expect("write base file");
        run_hg(&repo_dir, &["add", "big.txt"]);
        run_hg(
            &repo_dir,
            &["commit", "-m", "init", "-u", "tester <tester@local>"],
        );

        let mut big_content = String::new();
        for i in 0..500 {
            big_content.push_str(&format!("line-{i}\n"));
        }
        fs::write(repo_dir.join("big.txt"), big_content).expect("write modified file");

        let mut app = make_app();
        app.hg = Arc::new(CliHgClient::new(repo_dir.clone()));
        app.focus = FocusPanel::Files;

        app.refresh_snapshot(false);
        let snapshot_event = tokio::time::timeout(Duration::from_secs(5), app.event_rx.recv())
            .await
            .expect("snapshot timeout")
            .expect("snapshot event");
        app.handle_app_event(snapshot_event);

        let detail_event = tokio::time::timeout(Duration::from_secs(5), app.event_rx.recv())
            .await
            .expect("detail timeout")
            .expect("detail event");
        app.handle_app_event(detail_event);

        assert!(
            !app.snapshot.files.is_empty(),
            "expected modified file in snapshot"
        );
        let max_scroll = app.max_detail_scroll();
        assert!(max_scroll > 0, "expected overflowing details diff");

        for _ in 0..(max_scroll + 25) {
            app.handle_mouse(scroll_down(2, 15));
        }
        assert_eq!(app.details_scroll, max_scroll);

        app.last_refresh = Instant::now() - Duration::from_secs(8);
        app.periodic_refresh();
        let periodic_snapshot = tokio::time::timeout(Duration::from_secs(5), app.event_rx.recv())
            .await
            .expect("periodic snapshot timeout")
            .expect("periodic snapshot event");
        app.handle_app_event(periodic_snapshot);

        assert_eq!(app.details_scroll, max_scroll);

        fs::remove_dir_all(&repo_dir).ok();
    }

    #[test]
    fn detail_line_count_includes_trailing_newline_segment() {
        let mut app = make_app();
        app.detail_text = "a\nb\n".to_string();
        assert_eq!(app.detail_line_count(), 3);
    }

    #[test]
    fn max_detail_scroll_counts_trailing_newline() {
        let mut app = make_app();
        let lines = (0..13)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.detail_text = format!("{lines}\n");
        assert_eq!(app.max_detail_scroll(), 1);
    }

    #[test]
    fn max_detail_scroll_without_trailing_newline_is_unchanged() {
        let mut app = make_app();
        app.detail_text = (0..13)
            .map(|i| format!("line-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(app.max_detail_scroll(), 0);
    }

    #[test]
    fn custom_command_templates_render_selected_context() {
        let mut app = make_app();
        app.snapshot.repo_root = Some("/repo".to_string());
        app.snapshot.branch = Some("default".to_string());
        app.snapshot.files = vec![crate::domain::FileChange {
            path: "src/main.rs".to_string(),
            status: crate::domain::FileStatus::Modified,
        }];
        app.snapshot.revisions = vec![crate::domain::Revision {
            rev: 42,
            node: "abcdef0123456789".to_string(),
            desc: "msg".to_string(),
            user: "u".to_string(),
            branch: "default".to_string(),
            phase: "draft".to_string(),
            tags: Vec::new(),
            bookmarks: Vec::new(),
            date_unix_secs: 0,
        }];
        let mut env = HashMap::new();
        env.insert("TARGET".to_string(), "{rev}".to_string());
        let command = CustomCommand {
            id: "demo".to_string(),
            title: "Demo".to_string(),
            context: CommandContext::Repo,
            command: "echo {repo_root}".to_string(),
            args: vec![
                "{branch}".to_string(),
                "{file}".to_string(),
                "{node}".to_string(),
            ],
            env,
            show_output: true,
            needs_confirmation: false,
        };

        let run = app
            .prepare_custom_run_action(&command)
            .expect("custom command");
        assert_eq!(run.invocation.program, "echo");
        assert_eq!(
            run.invocation.args,
            vec![
                "/repo".to_string(),
                "default".to_string(),
                "src/main.rs".to_string(),
                "abcdef0123456789".to_string()
            ]
        );
        assert_eq!(
            run.invocation.env,
            vec![("TARGET".to_string(), "42".to_string())]
        );
    }

    #[test]
    fn custom_command_file_context_requires_selected_file() {
        let mut app = make_app();
        app.snapshot.repo_root = Some("/repo".to_string());
        let command = CustomCommand {
            id: "file-only".to_string(),
            title: "FileOnly".to_string(),
            context: CommandContext::File,
            command: "echo {file}".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            show_output: true,
            needs_confirmation: false,
        };
        let err = app
            .prepare_custom_run_action(&command)
            .expect_err("requires file selection");
        assert!(err.contains("file-context"));
    }

    #[test]
    fn custom_command_revision_context_requires_selected_revision() {
        let mut app = make_app();
        app.snapshot.repo_root = Some("/repo".to_string());
        let command = CustomCommand {
            id: "rev-only".to_string(),
            title: "RevOnly".to_string(),
            context: CommandContext::Revision,
            command: "echo {rev}".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            show_output: true,
            needs_confirmation: false,
        };
        let err = app
            .prepare_custom_run_action(&command)
            .expect_err("requires revision selection");
        assert!(err.contains("revision-context"));
    }

    #[test]
    fn custom_command_revision_context_populates_rev_and_node() {
        let mut app = make_app();
        app.snapshot.repo_root = Some("/repo".to_string());
        app.snapshot.revisions = vec![crate::domain::Revision {
            rev: 9,
            node: "deadbeef".to_string(),
            desc: "msg".to_string(),
            user: "u".to_string(),
            branch: "default".to_string(),
            phase: "draft".to_string(),
            tags: Vec::new(),
            bookmarks: Vec::new(),
            date_unix_secs: 0,
        }];
        let mut env = HashMap::new();
        env.insert("REV".to_string(), "{rev}".to_string());
        let command = CustomCommand {
            id: "rev".to_string(),
            title: "Rev".to_string(),
            context: CommandContext::Revision,
            command: "echo {node}".to_string(),
            args: Vec::new(),
            env,
            show_output: true,
            needs_confirmation: false,
        };
        let run = app
            .prepare_custom_run_action(&command)
            .expect("revision command renders");
        assert_eq!(run.invocation.program, "echo");
        assert_eq!(run.invocation.args, vec!["deadbeef".to_string()]);
        assert_eq!(
            run.invocation.env,
            vec![("REV".to_string(), "9".to_string())]
        );
    }

    #[test]
    fn custom_command_repo_context_uses_selected_revision_fallback_vars() {
        let mut app = make_app();
        app.snapshot.repo_root = Some("/repo".to_string());
        app.snapshot.revisions = vec![crate::domain::Revision {
            rev: 11,
            node: "cafebabe".to_string(),
            desc: "msg".to_string(),
            user: "u".to_string(),
            branch: "default".to_string(),
            phase: "draft".to_string(),
            tags: Vec::new(),
            bookmarks: Vec::new(),
            date_unix_secs: 0,
        }];
        let command = CustomCommand {
            id: "repo-with-rev-fallback".to_string(),
            title: "RepoWithRevFallback".to_string(),
            context: CommandContext::Repo,
            command: "echo".to_string(),
            args: vec!["{rev}".to_string(), "{node}".to_string()],
            env: HashMap::new(),
            show_output: true,
            needs_confirmation: false,
        };
        let run = app
            .prepare_custom_run_action(&command)
            .expect("repo fallback vars");
        assert_eq!(run.invocation.program, "echo");
        assert_eq!(
            run.invocation.args,
            vec!["11".to_string(), "cafebabe".to_string()]
        );
    }

    #[test]
    fn open_command_palette_no_commands_sets_status() {
        let mut app = make_app();
        app.config.custom_commands = Vec::new();
        app.open_command_palette();
        assert!(app.command_palette.is_none());
        assert!(app.status_line.contains("No custom commands configured"));
    }

    #[test]
    fn toggle_file_selection_adds_and_removes_path() {
        let mut app = make_app();
        app.snapshot.files = vec![crate::domain::FileChange {
            path: "src/main.rs".to_string(),
            status: crate::domain::FileStatus::Modified,
        }];
        app.files_idx = 0;

        app.toggle_selected_file_for_commit();
        assert!(app.is_file_selected_for_commit("src/main.rs"));
        assert_eq!(app.selected_file_commit_count(), 1);

        app.toggle_selected_file_for_commit();
        assert!(!app.is_file_selected_for_commit("src/main.rs"));
        assert_eq!(app.selected_file_commit_count(), 0);
    }

    #[test]
    fn clear_file_selection_empties_selection() {
        let mut app = make_app();
        app.commit_file_selection.insert("a".to_string());
        app.commit_file_selection.insert("b".to_string());
        app.clear_file_selection();
        assert_eq!(app.selected_file_commit_count(), 0);
        assert!(app.status_line.contains("Cleared commit file selection"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_commit_action_event_clears_selected_files() {
        let mut app = make_app();
        app.commit_file_selection.insert("src/app.rs".to_string());
        app.handle_app_event(AppEvent::ActionFinished {
            action_preview: "hg commit -m <message> <1 files>".to_string(),
            show_output: false,
            clear_commit_selection: true,
            result: Ok(CommandResult {
                command_preview: "hg commit -m test src/app.rs".to_string(),
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            }),
        });
        assert_eq!(app.selected_file_commit_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_commit_action_event_keeps_selected_files() {
        let mut app = make_app();
        app.commit_file_selection.insert("src/app.rs".to_string());
        app.handle_app_event(AppEvent::ActionFinished {
            action_preview: "hg commit -m <message> <1 files>".to_string(),
            show_output: false,
            clear_commit_selection: true,
            result: Ok(CommandResult {
                command_preview: "hg commit -m test src/app.rs".to_string(),
                success: false,
                stdout: String::new(),
                stderr: "abort: no username configured".to_string(),
            }),
        });
        assert_eq!(app.selected_file_commit_count(), 1);
    }

    #[test]
    fn interactive_commit_input_creates_request() {
        let mut app = make_app();
        app.commit_file_selection.insert("src/app.rs".to_string());
        app.input = Some(InputState {
            title: "Interactive".to_string(),
            value: "msg".to_string(),
            purpose: InputPurpose::CommitMessageInteractive,
        });
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.handle_input_key(enter));
        let request = app
            .interactive_commit_request
            .as_ref()
            .expect("interactive request created");
        assert_eq!(request.message, "msg");
        assert_eq!(request.files, vec!["src/app.rs".to_string()]);
    }

    #[test]
    fn empty_input_keeps_modal_open_for_retry() {
        let mut app = make_app();
        app.input = Some(InputState {
            title: "Commit".to_string(),
            value: "   ".to_string(),
            purpose: InputPurpose::CommitMessage,
        });
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.handle_input_key(enter));
        assert!(app.input.is_some());
        assert_eq!(app.status_line, "Input cannot be empty.");
    }

    #[test]
    fn parse_command_parts_supports_quotes_and_escapes() {
        let (program, args) =
            parse_command_parts(r#"cmd --message "hello world" --path 'src/main.rs' plain\ arg"#)
                .expect("parse command");
        assert_eq!(program, "cmd");
        assert_eq!(
            args,
            vec![
                "--message".to_string(),
                "hello world".to_string(),
                "--path".to_string(),
                "src/main.rs".to_string(),
                "plain arg".to_string(),
            ]
        );
    }

    #[test]
    fn parse_command_parts_rejects_unmatched_quote() {
        let err = parse_command_parts(r#"echo "broken"#).expect_err("reject unmatched quote");
        assert!(err.contains("unmatched quote"));
    }

    #[test]
    fn double_click_requires_same_target_within_threshold() {
        let mut app = make_app();
        app.last_mouse_click = Some(LastMouseClick {
            panel: FocusPanel::Files,
            index: Some(1),
            button: MouseButton::Left,
            at: Instant::now(),
        });
        assert!(app.is_double_click(FocusPanel::Files, Some(1), MouseButton::Left));
        assert!(!app.is_double_click(FocusPanel::Files, Some(2), MouseButton::Left));
        app.last_mouse_click = Some(LastMouseClick {
            panel: FocusPanel::Files,
            index: Some(1),
            button: MouseButton::Left,
            at: Instant::now() - Duration::from_millis(DOUBLE_CLICK_THRESHOLD_MS + 5),
        });
        assert!(!app.is_double_click(FocusPanel::Files, Some(1), MouseButton::Left));
    }
}
